# Inline vs Offloaded Work Classification & Metadata Publication Primitives

**Beads:** `bd-db300.5.4.1` (E4.1) + `bd-db300.5.3.1` (E3.1)
**Date:** 2026-03-22
**Status:** Design contract — primitive choices and implementation touchpoints fixed for downstream E3/E4 work
**Depends on:** E1 state-placement map, ADR-0002 (tiny-publish shared state), B3 (lock-free publication work)

---

## Purpose

This document delivers two tightly coupled artifacts:

1. **E4.1 — Inline vs offloaded work classification**: partition every operation
   on the writer commit path into one of four execution classes (INLINE-CRITICAL,
   INLINE-FAST, OFFLOAD-ASYNC, OFFLOAD-BACKGROUND), tied to latency ownership
   and queue-budgeting constraints.

2. **E3.1 — Metadata class publication map**: partition every reader-visible
   metadata surface by read-write ratio, retryability, snapshot semantics, and
   reclamation difficulty, so E3.2 can select publication primitives without
   reopening architectural questions.

These two classifications are coupled because the publication primitive chosen
for a metadata class constrains whether the corresponding publication work
must be inline or can be offloaded — and conversely, a work-class decision
constrains which publication primitives are available.

---

## Grounding

### Architecture: Tiny-Publish Shared State (ADR-0002)

The selected Track E architecture keeps Stages 1–3 lane-local, makes Stage 4
(first-touch arbitration) narrow, concentrates all irreducible global work into
a tiny Stage 5 publish window, and pushes everything else to asynchronous
Stage 6. This classification must not widen Stages 4 or 5.

### Evidence: Live Pipeline (E1 State Placement)

The live pipeline stages, ownership boundaries, and shared touch points are
documented in `docs/design/many-core-transaction-pipeline-state-placement.md`.
The invariants INV-DB300-E1.1-1 through E1.1-8 apply throughout.

### Evidence: Measured Performance Facts

From the performance program (2026-03-21 snapshot):
- c1 disjoint: 0.205x (dominated by fixed-cost overhead, not contention)
- c4 disjoint: 0.553x (contention tax prevents inheriting c8 geometry)
- c8 disjoint: 3.140x (enough parallelism to amortize shared surfaces)
- c4 mixed: 0.963x (near parity, publication overhead starting to show)
- c8 mixed: 5.903x (publication cost amortized by parallelism)

The c4 gap relative to c8 is the primary target. Work that runs inline at c4
but could be offloaded represents the highest-EV scheduling lever.

---

## Part 1: Inline vs Offloaded Work Classification (E4.1)

### Execution Classes

| Class | Abbreviation | Contract | Latency Budget |
|-------|-------------|----------|----------------|
| **INLINE-CRITICAL** | IC | Must complete before commit is visible. Failure aborts the transaction. Runs on writer thread inside the publish window. | ≤ 500ns per page at p99 |
| **INLINE-FAST** | IF | Must complete before control returns to the application. May run after publish but before `COMMIT` returns. Failure is logged, not fatal. | ≤ 5μs total at p99 |
| **OFFLOAD-ASYNC** | OA | May complete after `COMMIT` returns. Must complete before the next checkpoint or visibility-critical event. Failure triggers retry or degraded-mode fallback. | ≤ 100μs, amortized |
| **OFFLOAD-BACKGROUND** | OB | Best-effort. May be batched, delayed, or dropped under load. Failure is observable but not fatal. | Unbounded, but budgeted queue depth |

### Classification Table

Each row is a concrete operation on the writer commit path, classified by
execution class, the shared surface it touches, and the invariant it serves.

#### Stage 2: Transaction Admission

| Operation | Class | Shared Surface | Invariant | Rationale |
|-----------|-------|---------------|-----------|-----------|
| Bind pager publication snapshot | IC | PagerPublishedSnapshot | INV-E1.1-2 (snapshot validity) | Snapshot must be bound before any commit can advance past it. Cannot be deferred. |
| Register concurrent session in ConcurrentRegistry | IF | ConcurrentRegistry Mutex | INV-E1.1-1 (concurrent-by-default) | Registration must complete before first statement executes. Not critical-path for other writers. |

#### Stage 3: Statement Execution

| Operation | Class | Shared Surface | Invariant | Rationale |
|-----------|-------|---------------|-----------|-----------|
| Already-owned page writes | — (lane-local) | None | — | No shared interaction. Pure local work. |
| Read from CommitIndex for visibility check | IC | CommitIndex.fast_array (Acquire load) | INV-E1.1-4 (monotonicity) | Single atomic load per page. Already O(1), no lock. |

#### Stage 4: First-Touch Arbitration

| Operation | Class | Shared Surface | Invariant | Rationale |
|-----------|-------|---------------|-----------|-----------|
| Page lock acquisition (CAS on fast array) | IC | InProcessPageLockTable fast_array | INV-E1.1-3 (first-touch exclusivity) | Single CAS instruction. Must succeed or fail before the page can be written. |
| Page lock acquisition (sharded fallback, page > 65536) | IC | InProcessPageLockTable shard Mutex | INV-E1.1-3 | Rare path. Shard Mutex held briefly. Still inline because ownership must be resolved before write proceeds. |
| CommitIndex staleness check on first touch | IC | CommitIndex.fast_array (Acquire load) | INV-E1.1-4 | FCW precondition: must verify no concurrent commit advanced this page since snapshot. |
| Page-1 structural conflict tracking | IC | Synthetic conflict surface | INV-E1.1-6 (structural explicitness) | Rare path. Must be resolved before commit planning because it widens the conflict surface. |
| Wait-for-page-lock-holder-change (parking) | IC | Lock table waiter queue | INV-E1.1-3 | Writer is blocked. Cannot proceed until lock is available. Not offloadable — the writer IS the waiter. |

#### Stage 5: Commit Planning, Durable Commit, and Publish

| Operation | Class | Shared Surface | Invariant | Rationale |
|-----------|-------|---------------|-----------|-----------|
| SSI validation (FCW check against CommitIndex) | IC | CommitIndex (reads), ConcurrentRegistry (Mutex) | INV-E1.1-4, INV-E1.1-5 | Must determine commit/abort before durable order is assigned. |
| Pager txn.commit() — WAL frame write | IC | Pager internal state, WAL file | INV-E1.1-5 (publish-after-durable) | Durable write is irreducibly on the critical path. The write is already committed to durable storage when this returns. |
| advance_commit_clock() — atomic fetch_add on next_commit_seq | IC | next_commit_seq (AtomicU64 AcqRel) | INV-E1.1-5 | Irreducibly global. Single instruction. The durable-order allocator. |
| CommitIndex.batch_update() — publish visibility | IC | CommitIndex.fast_array (Relaxed stores after Release fence) | INV-E1.1-4, INV-E1.1-5 | Must complete before lock release so readers see consistent state. |
| Page lock release (CAS on fast array) | IC | InProcessPageLockTable fast_array | INV-E1.1-3 | Must follow visibility publication. Releases ownership to waiting writers. |
| Lock-release waiter wakeup | IF | Lock table waiter queue | — | Wakeup can be slightly delayed. Waiters are parking, not spinning. Moving this after the critical section reduces publish-window duration. |
| SSI abort decision + evidence snapshot | IC (abort path only) | ConcurrentRegistry | — | Only on abort. Must complete before returning BUSY to caller. |
| SSI commit evidence card recording | OA | Evidence ledger | — | Already async via `record_async()`. Observable audit trail, not correctness-critical. |

#### Stage 6: Post-Commit Cleanup

| Operation | Class | Shared Surface | Invariant | Rationale |
|-----------|-------|---------------|-----------|-----------|
| Active txn handle drop + RefCell cleanup | IF | None (lane-local) | — | Must complete before next statement begins. Trivial cost. |
| VTab commit notification | OA | Extension vtab state | — | Best-effort. Extensions should not block the commit return. |
| MemDatabase staleness update | IF | memdb_visible_commit_seq (RefCell, lane-local) | — | Lane-local update. Trivial. |
| Differential commit invalidation emission | OA | Invalidation channel | — | Readers will pick up invalidation on next snapshot bind. Does not need to be synchronous. |
| Time-travel snapshot capture | OB | Time-travel ring buffer (lane-local) | — | Diagnostic/debug feature. May be expensive (clones entire MemDatabase). |
| Adaptive autocheckpoint | OB | Pager checkpoint state | INV-E1.1-7 (reclamation) | Already a separate concern. Should never block commit return. Must respect WAL size bounds eventually, but not synchronously. |
| MVCC GC tick | OB | VersionStore, VersionGuardRegistry | INV-E1.1-7 | Reclamation is correctness work but not on the commit critical path. Must respect active snapshot horizon. |
| ConcurrentRegistry session recycle | IF | ConcurrentRegistry Mutex | — | Handle recycle is fast (remove from HashMap, push to free list). Slightly delays Mutex release but avoids a second lock acquisition. |

### Publish-Window Composition

The INLINE-CRITICAL publish window (Stage 5, from SSI validation through lock
release) currently contains:

```
┌─────────────────── PUBLISH WINDOW ───────────────────┐
│                                                       │
│  SSI validation (reads: CommitIndex, registry)        │  IC
│  pager txn.commit() (WAL write)                       │  IC — DOMINATES
│  advance_commit_clock (1 atomic fetch_add)            │  IC — ~1ns
│  CommitIndex.batch_update (1 fence + N relaxed stores)│  IC — ~5ns/page
│  page lock release (N CAS operations)                 │  IC — ~5ns/page
│                                                       │
└───────────────────────────────────────────────────────┘
  THEN (outside window):
    waiter wakeup                                          IF
    session recycle                                        IF
    evidence recording                                     OA
    snapshot capture, GC, checkpoint                        OB
```

**Key observation:** The pager `txn.commit()` (WAL frame write) dominates the
publish window when I/O is involved (file-backed DBs). For `:memory:` databases,
the atomic operations and CommitIndex updates dominate. The c4→c8 scaling gap is
most likely caused by contention on `ConcurrentRegistry` Mutex during SSI
validation, not by the atomic operations themselves.

### Inline→Offload Promotion Candidates

Operations that are currently INLINE but could be promoted to OFFLOAD with
careful design:

| Operation | Current | Target | Precondition | Risk |
|-----------|---------|--------|--------------|------|
| Waiter wakeup | IC (inside lock release) | IF (after publish window) | Lock release CAS is separate from wakeup dispatch | Low: waiters are already parking, 1–5μs delay is invisible |
| SSI evidence recording | OA (already) | OA | Already correct | None |
| VTab notification | IF | OA | VTab state must tolerate delayed notification | Low: extensions already handle async |
| Checkpoint | OB (already) | OB | Already correct | None |
| GC tick | OB (already) | OB | Already correct | None |

Operations that MUST NOT be promoted:

| Operation | Why |
|-----------|-----|
| SSI validation | Determines commit/abort. Cannot defer. |
| Pager WAL write | Durability contract. Cannot skip. |
| Commit clock advance | Durable order assignment. Irreducibly global. |
| CommitIndex publication | Readers must see committed state before lock release. |
| Lock release | Must follow publication. Waiters depend on it. |

### Fallback Behavior

If the scheduling system cannot classify an operation (e.g., a new path is added
without classification), the safe fallback is:

- **Default: INLINE-FAST.** This is correct (never delays visibility) and
  observable (adds latency to commit return, which will show in benchmarks).
- **Never default to OFFLOAD.** An incorrectly offloaded operation can violate
  visibility or ordering invariants silently.

---

## Part 2: Metadata Class Publication Map (E3.1)

### Classification Axes

Each metadata class is scored on five axes:

| Axis | Description | Scale |
|------|-------------|-------|
| **Read:Write ratio** | How many readers per write | R:W ratio (higher = more read-heavy) |
| **Retryability** | Can a reader retry a stale/torn read? | Yes / Conditional / No |
| **Snapshot semantics** | Must readers see a consistent point-in-time snapshot? | Strict / Relaxed / None |
| **Reclamation** | Does old metadata need explicit lifecycle management? | EBR / Grace / Immediate / None |
| **Topology sensitivity** | Does cross-NUMA access to this metadata materially hurt? | High / Medium / Low |

### Metadata Classes

#### Class M1: Per-Page Commit Visibility (CommitIndex)

| Axis | Value | Evidence |
|------|-------|----------|
| Read:Write | ~100:1 to ~1000:1 | Every first-touch read checks CommitIndex; writes happen only at commit |
| Retryability | **Yes** | Readers can retry: a stale read just means the reader doesn't see the latest commit, which is correct under snapshot semantics |
| Snapshot semantics | **Relaxed** | Monotonicity is required (INV-E1.1-4), but readers don't need a consistent cross-page snapshot of the CommitIndex — they read per-page |
| Reclamation | **None** | CommitIndex entries are overwritten in place (atomic store). No old-version lifecycle. |
| Topology sensitivity | **High** | Hot pages cause cache-line bouncing between writer (store) and readers (load) across NUMA nodes |

**Current primitive:** Tier-1 flat AtomicU64 array (pages ≤ 65536) + Tier-2 sharded LeftRight (pages > 65536).

**Publication constraint:** Writer must issue Release fence before stores, reader must issue Acquire load. Stores are Relaxed after the fence. This is already optimal for the single-writer-many-reader pattern per page.

**Observation for E3.2:** The flat array is already near-optimal. The LeftRight tier for large pages adds unnecessary Mutex overhead. A sharded epoch-reclaimed concurrent hash map or extending the flat array to 262144 entries (2 MiB) would eliminate the slow tier.

---

#### Class M2: Page Ownership Directory (InProcessPageLockTable)

| Axis | Value | Evidence |
|------|-------|----------|
| Read:Write | ~1:1 | Each first-touch acquires a lock (write); each commit releases it (write). Reads happen during FCW validation. |
| Retryability | **No** | Lock acquisition is a CAS — it either succeeds or the writer must wait/abort. Not retryable in the seqlock sense. |
| Snapshot semantics | **Strict** | Lock state must be exact: either this txn owns the page or it doesn't. Stale reads cause lost-update bugs. |
| Reclamation | **None** | Locks are released in place (atomic store to 0). No version chain. |
| Topology sensitivity | **High** | Contended CAS on the same cache line between writers on different NUMA nodes is the worst-case cross-node penalty. |

**Current primitive:** Tier-1 flat AtomicU64 array (CAS) + Tier-2 sharded Mutex<HashMap>.

**Publication constraint:** CAS is inherently linearizable. No weaker primitive is safe here. The only optimization axis is reducing contention (via conflict-geometry routing, E5) or reducing the number of first-touch events (via page-affinity hints).

**Observation for E3.2:** This is already at the correct primitive. The E5 (conflict-topology routing) and E6 (lane placement) beads are the right levers, not a publication primitive change.

---

#### Class M3: Global Commit Sequence Counter (next_commit_seq)

| Axis | Value | Evidence |
|------|-------|----------|
| Read:Write | ~0:1 | Written once per commit. Readers don't read this directly — they read CommitIndex (M1) or snapshot bindings (M5). |
| Retryability | **N/A** | Not read in the hot path. |
| Snapshot semantics | **Strict** | Commit ordering is a total order. Must be monotonically increasing. |
| Reclamation | **None** | Single counter, overwritten atomically. |
| Topology sensitivity | **High** | fetch_add on a single cache line from multiple NUMA nodes is the textbook worst case. Already batched via CommitSequenceCombiner. |

**Current primitive:** AtomicU64 with AcqRel fetch_add, batched through CommitSequenceCombiner (flat combining).

**Publication constraint:** Total order is required. The combiner already reduces cache-line traffic by ~16x under load. Further reduction requires either HTM (bd-77l3t) or per-NUMA-node pre-allocation with global reconciliation.

**Observation for E3.2:** The combiner is the correct primitive. HTM fast-path (bd-77l3t) is the natural next optimization. Per-NUMA pre-allocation adds complexity with marginal gain given combiner batching already works.

---

#### Class M4: Concurrent Transaction Registry (ConcurrentRegistry)

| Axis | Value | Evidence |
|------|-------|----------|
| Read:Write | ~1:2 | Reads during SSI validation (once per commit for each active txn). Writes: begin (1), plan (1), finalize (1), recycle (1). |
| Retryability | **No** | SSI validation must see the exact set of active transactions. A stale view causes false negatives (missed conflicts). |
| Snapshot semantics | **Strict** | The registry snapshot during SSI validation must be linearizable with respect to concurrent commits. |
| Reclamation | **Immediate** | Session handles are recycled on finalize. No deferred reclamation needed. |
| Topology sensitivity | **Medium** | Mutex is held briefly. Cross-NUMA penalty exists but is amortized by the (relatively) long SSI validation work inside the lock. |

**Current primitive:** `TransactionManager`-owned `Mutex<ConcurrentRegistry>` protecting the active-session `HashMap` and committed SSI indexes, plus per-handle `Mutex<ConcurrentHandle>` leaves.

**Publication constraint:** SSI validation must atomically observe: (a) which transactions are active, (b) their read/write sets, (c) their commit status. This is inherently a consistent-snapshot problem.

**Observation for E3.2:** The global Mutex is the most promising optimization target for c4. Options:
1. **Sharded registry** by session-id hash — reduces lock contention but complicates SSI scan.
2. **RCU-style snapshot** — writers publish via copy-on-write; readers see a consistent snapshot without blocking writers. Reclamation via epoch.
3. **Seqlock + per-session atomics** — readers retry if registry changed during scan. Works if SSI scan is fast relative to commit rate.

Recommendation: Option 2 (RCU snapshot) for E3.2/E3.3, because SSI validation is read-dominant during the critical window, the active-session set is bounded by core count, and long scans want an immutable view rather than a seqlock that can livelock under repeated writer churn. The code contract for this choice now lives in `MVCC_METADATA_PUBLICATION_CONTRACTS`.

---

#### Class M5: Pager Publication Snapshot

| Axis | Value | Evidence |
|------|-------|----------|
| Read:Write | ~N:1 (N = concurrent readers per commit) | Written once per commit. Read by every transaction at BEGIN to bind snapshot. |
| Retryability | **Yes** | A reader that sees a slightly old snapshot just starts from an earlier consistent state. Safe under MVCC. |
| Snapshot semantics | **Relaxed** | Must be internally consistent (commit_seq matches page_set_size), but a reader seeing the previous snapshot is correct. |
| Reclamation | **None** | Overwritten in place. |
| Topology sensitivity | **Medium** | Read on BEGIN, not on every page access. Amortized over transaction lifetime. |

**Current primitive:** writer-serialized seqlock-style publication plane in `PublishedPagerState`: readers sample `sequence`, read the summary atomics, and retry if the sequence is odd or changed; targeted waiters provide bounded parking when a writer is in-flight.

**Publication constraint:** Internal consistency required (multi-field snapshot). Relaxed freshness is acceptable.

**Observation for E3.2/E3.3:** The pager already implements the right shape: a seqlock-style multi-field summary with explicit retry and a separate lag-detecting page-plane horizon. The contract is to preserve that split rather than collapsing everything into an RCU copy or a reader lock. The code contract for this choice now lives in `PAGER_METADATA_PUBLICATION_CONTRACTS`.

---

#### Class M5b: Shared Durable Snapshot Triple (`ShmSnapshot`)

| Axis | Value | Evidence |
|------|-------|----------|
| Read:Write | ~N:1 | Read by transaction admission / recovery; written once per publish/reconcile |
| Retryability | **Yes** | Readers can spin/retry while a writer publishes the triple |
| Snapshot semantics | **Strict** | `(commit_seq, schema_epoch, ecs_epoch)` must be mutually consistent |
| Reclamation | **None** | Triple fields are overwritten in place |
| Topology sensitivity | **Medium** | Shared cross-process header line, but the payload is only three `u64`s |

**Current primitive:** explicit seqlock triple in `shm.rs::SharedMemoryLayout::{publish_snapshot, load_consistent_snapshot}`.

**Publication constraint:** `schema_epoch` must publish before `commit_seq` on DDL paths, and readers must never combine fields from different generations.

**Observation for E3.2/E3.3:** Keep the seqlock triple. RCU would allocate and reclaim an object per commit for a three-word payload, and independent atomics would allow torn reads.

---

#### Class M6: Schema/Pragma Metadata

| Axis | Value | Evidence |
|------|-------|----------|
| Read:Write | ~10000:1 | Schema changes are rare (DDL). Reads happen on every statement for schema validation. |
| Retryability | **Yes** | A reader seeing a slightly stale schema will recompile the statement, which is correct (schema epoch check). |
| Snapshot semantics | **Relaxed** | Schema epoch is sufficient for staleness detection. No cross-field consistency needed. |
| Reclamation | **None** | Schema data is owned by connection-local registries. Global publication is epoch-only. |
| Topology sensitivity | **Low** | Single atomic read per statement (schema epoch check). Extremely low traffic. |

**Current primitive:** standalone schema invalidation reads use a monotone
epoch (`lifecycle.rs::schema_epoch`, `shm.rs::load_schema_epoch`), while the
cross-process `(commit_seq, schema_epoch, ecs_epoch)` publication is bundled
into the M5b seqlock triple.

**Publication constraint:** if a reader only needs staleness detection, a
single monotone epoch is sufficient. If a reader needs `schema_epoch` coherent
with `commit_seq` or `ecs_epoch`, it must bind through the M5b triple and not
sample the fields independently.

**Observation for E3.2:** keep schema metadata split by coupling. Use a single
atomic epoch for invalidation-only reads; use the M5b seqlock triple whenever
schema state is part of a durable multi-field snapshot. RCU, Left-Right, and
BRAVO are unjustified here because the payload is scalar and the schema objects
themselves remain connection-local.

---

#### Class M7: Version Store (Committed Page Versions)

| Axis | Value | Evidence |
|------|-------|----------|
| Read:Write | ~5:1 to ~50:1 | Written once per page per commit. Read by every transaction that needs a historical page version. |
| Retryability | **No** | A reader traversing the version chain must see consistent versions. Torn reads cause data corruption. |
| Snapshot semantics | **Strict** | Version chains must be consistent with CommitIndex visibility. A reader must never see a partial version entry. |
| Reclamation | **EBR** | Old versions must persist until no active snapshot can see them. Epoch-based reclamation (crossbeam-epoch) is the correct discipline. |
| Topology sensitivity | **Medium** | Version chain traversal is pointer-chasing, which is inherently NUMA-sensitive. But this is read-only for committed versions. |

**Current primitive:** Shared concurrent structure with epoch-based GC via gc_tick.

**Publication constraint:** Append-only for committed versions. GC is the only mutating operation on historical entries. The publication primitive must guarantee that a newly committed version is visible before the corresponding CommitIndex entry is published.

**Observation for E3.2:** The current structure is correct but should ensure that version store insertion happens-before CommitIndex publication (which it does: `finalize_prepared_concurrent_commit_with_ssi` publishes versions before CommitIndex batch_update). The GC path (bd-wnk1r, bd-bolsv) is the optimization target, not the publication primitive.

---

#### Class M7b: Reclamation Horizon and Reader Pins

| Axis | Value | Evidence |
|------|-------|----------|
| Read:Write | ~N:1 | GC and cleanup read the floor frequently; commits and pin/unpin events advance it |
| Retryability | **Conditional** | A stale low horizon is safe; a stale high horizon is not |
| Snapshot semantics | **Relaxed monotone** | Consumers need a safe lower bound, not a globally consistent multi-field snapshot |
| Reclamation | **Self-gating** | The horizon itself is not reclaimed; it gates reclamation of other objects |
| Topology sensitivity | **Medium** | Shared floor traffic exists, but the operations are narrow atomics and min-reductions |

**Current primitive:** monotone atomics plus active-slot and epoch-pin floors in `core_types.rs::raise_gc_horizon` and `ebr.rs::VersionGuardRegistry::min_pinned_epoch`.

**Publication constraint:** the horizon may advance only when every older reader pin and active transaction snapshot has moved past the retire epoch.

**Observation for E3.2/E3.3:** Do not wrap the horizon in RCU or seqlock. Consumers only need a safe floor; stale-low reads are acceptable and cheaper than retrying a synthetic snapshot.

---

#### Class M8: SSI Evidence and Observability

| Axis | Value | Evidence |
|------|-------|----------|
| Read:Write | ~0.01:1 | Written on every commit/abort. Read only by diagnostic tools and the abort-rate controller. |
| Retryability | **Yes** | Evidence is informational. Stale reads cause suboptimal policy decisions, not correctness violations. |
| Snapshot semantics | **None** | Best-effort. Loss of individual evidence cards is acceptable. |
| Reclamation | **None** | Ring buffer or append log with bounded size. Oldest entries are overwritten. |
| Topology sensitivity | **Low** | Written once, read rarely. No hot cache-line contention. |

**Current primitive:** Async recording via `record_async()`.

**Publication constraint:** Already correct. Fire-and-forget is the right model. The abort-rate controller (bd-3t52f) will read this asynchronously.

**Observation for E3.2:** No change needed.

---

### Metadata Class Summary Matrix

| Class | Surface | R:W | Retry | Snapshot | Reclaim | NUMA | Current Primitive | Candidate Upgrade |
|-------|---------|-----|-------|----------|---------|------|-------------------|-------------------|
| M1 | CommitIndex | 100:1+ | Yes | Relaxed | None | High | AtomicU64 array + LeftRight | Extend flat array; replace LeftRight with epoch hash |
| M2 | PageLockTable | 1:1 | No | Strict | None | High | AtomicU64 array + shard Mutex | No primitive change; routing (E5) is the lever |
| M3 | next_commit_seq | 0:1 | N/A | Strict | None | High | AtomicU64 + CommitSequenceCombiner | HTM fast-path (bd-77l3t) |
| M4 | ConcurrentRegistry | 1:2 | No | Strict | Immediate | Medium | registry Mutex + per-handle Mutex leaves | **Split into RCU active snapshot + append-only committed conflict ledger** |
| M5 | PagerPublishedSnapshot | N:1 | Yes | Relaxed | None | Medium | writer-serialized seqlock summary + lagging page-plane horizon | Keep seqlock summary + explicit fallback |
| M5b | ShmSnapshot triple | N:1 | Yes | Strict | None | Medium | explicit seqlock triple | Keep seqlock triple |
| M6 | Schema/Pragma epoch | 10000:1 | Yes | Relaxed | None | Low | monotone atomic epoch + M5b seqlock triple when coupled | Keep scalar invalidation epoch local; bind through M5b for durable multi-field reads |
| M7 | VersionStore | 5:1+ | No | Strict | EBR | Medium | Epoch-based concurrent store | GC optimization (WS3) |
| M7b | GC horizon / reader-pin floor | N:1 | Conditional | Relaxed monotone | Self-gating | Medium | monotone atomics + epoch floor | No primitive change |
| M8 | SSI Evidence | 0.01:1 | Yes | None | None | Low | Async ring buffer | Keep async telemetry; treat witness archive as separate append-only publication surface |

---

## E3.2 Primitive Selection Matrix

E3.1 classified the coarse metadata surfaces. E3.2 refines that inventory at
the actual publication boundaries that exist in code:

- M4 splits into the **active transaction snapshot table** and the
  **committed conflict ledger** because SSI scans and historical conflict
  lookups have different read/write shapes.
- M6 splits by coupling: a standalone schema invalidation epoch is a scalar,
  but the durable `(commit_seq, schema_epoch, ecs_epoch)` image is a coherent
  multi-field snapshot and therefore belongs with M5b.
- M8 splits low-value policy telemetry from the higher-fidelity witness
  archive because they tolerate very different publication costs.

### Primitive Heuristics

| Metadata shape | Preferred primitive | Why |
|----------------|---------------------|-----|
| Fixed-width multi-field summary with retryable reads | **Seqlock** | readers cheaply retry until they see a coherent generation |
| Immutable snapshot table scanned by readers | **RCU/QSBR** | readers bind one image and do not block writers |
| Append-only ledger / archive | **Atomic pointer swap or RCU-published immutable segments** | readers consume a prefix; writers never mutate published history in place |
| Exact ownership / exclusion directory | **Sharded atomics + CAS** | state must be linearizable and cannot tolerate stale reads |
| Monotone floor / horizon | **Single atomics with min-reduction** | stale-low is safe, so richer snapshot machinery only adds latency |

### Class-by-Class Mapping

| Class | Touchpoint | Selected Primitive | Candidates Considered | Explicitly Rejected | Why This Fits FrankenSQLite |
|-------|------------|--------------------|-----------------------|---------------------|-----------------------------|
| M1 CommitIndex | `core_types.rs::CommitIndex` | direct-indexed atomics for hot pages, sharded fallback for cold/large pages | Left-Right shards, RCU hash, seqlock summary | BRAVO rwlock, whole-map RLU-like copy, whole-array seqlock | per-page visibility is a single-word publish; readers can safely observe stale-old page visibility without needing a cross-page snapshot |
| M2 Page ownership directory | `begin_concurrent.rs::InProcessPageLockTable` | sharded atomic CAS directory with bounded handoff waiters | seqlock directory, BRAVO `RwLock`, RCU map, Left-Right copy | seqlock, RCU, Left-Right, BRAVO | first-touch ownership is exact and mutation-heavy; a writer must know whether it owns the page now, not eventually |
| M3 next_commit_seq | `shm.rs::next_commit_seq` plus `CommitSequenceCombiner` | flat-combined atomic `fetch_add` | HTM-guarded combiner, per-node reservation blocks | seqlock, Left-Right, RCU, BRAVO, RLU-like | the value is one globally ordered scalar; the only real lever is reducing cache-line traffic, not changing snapshot semantics |
| M3b Durable commit ledger | `core_types.rs::CommitLog` | append-only immutable segments with atomic tail publication | single locked `Vec` tail, whole-log Left-Right, seqlock over tail metadata | whole-log Left-Right, seqlock, BRAVO, RLU-like whole-log copy | readers only need a prefix-stable history; published records are immutable, so segment append plus atomic tail swap matches the access pattern better than copying or retry loops |
| M4a Active transaction snapshot table | `begin_concurrent.rs::ConcurrentRegistry::{active,active_snapshot_highs,gc_horizon_counts}` | RCU/QSBR-published immutable registry image with mutable per-handle leaves retained separately | global `Mutex`, sharded `Mutex`, whole-registry seqlock, Left-Right duplicate | whole-registry seqlock, BRAVO rwlock, long-term global `Mutex` | SSI validation wants one immutable view of the active set; readers should not hold the writer lock across conflict scans |
| M4b Committed conflict ledger | `begin_concurrent.rs::ConcurrentRegistry::{committed_readers*,committed_writers*}` | append-only shard-per-key-family ledgers published via immutable segment headers and epoch reclamation | global registry `Mutex`, Left-Right full duplicate, seqlock-protected vectors, RLU-like copied indexes | seqlock, BRAVO, whole-registry Left-Right | committed conflict history is append-only and lookup-heavy by page / exact cell, so immutable indexed segments fit better than retry-on-write schemes |
| M5 Pager snapshot summary | `pager.rs::PublishedPagerState::{snapshot,finalize_publish}` | seqlock-style summary publication | `RwLock` summary, atomic pointer swap of summary objects, Left-Right copies | BRAVO rwlock, Left-Right, RLU-like snapshot objects | readers sample a tiny fixed field set at BEGIN and may retry; allocation-heavy snapshot replacement is strictly worse than a stable seqlock summary |
| M5b SHM durable snapshot triple | `shm.rs::SharedMemoryLayout::{publish_snapshot,load_consistent_snapshot}` | seqlock triple | independent atomics, RCU object swap, Left-Right pair copies | independent atomics, Left-Right, BRAVO, RLU-like | readers must never combine `commit_seq`, `schema_epoch`, and `ecs_epoch` from different generations, but the payload is too small to justify copy-on-write |
| M6 Schema invalidation epoch | `lifecycle.rs::schema_epoch` and `shm.rs::load_schema_epoch` | monotone atomic epoch when standalone; piggyback on M5b when bundled with durable snapshot state | standalone seqlock, RCU schema object publication | Left-Right, BRAVO, standalone RCU copies | most readers only need staleness detection; the real schema objects are connection-local, so only the epoch needs publication |
| M7 VersionStore chain publication | `invariants.rs::VersionStore` plus `ebr.rs::VersionGuardRegistry` | CAS-published chain heads plus append-only EBR-protected nodes | full-copy RCU chains, seqlock-protected mutable chains, RLU-like node indirection | seqlock chains, BRAVO, whole-chain copy on commit | readers need stable historical chain traversal and delayed reclamation, not snapshot replacement of entire page histories |
| M7b GC horizon / reader pins | `core_types.rs::raise_gc_horizon` plus `VersionGuardRegistry::min_pinned_epoch` | monotone atomics with floor recomputation | seqlock floor, RCU floor snapshot, Left-Right duplicate floor | seqlock, RCU, BRAVO, Left-Right | consumers only need a safe lower bound; stale-low is correct and cheaper than any retry-based scheme |
| M8 SSI decision telemetry | `ssi_abort_policy.rs::record_async` | bounded async ring / offload lane | RCU observation list, seqlock counters, global mutex log | seqlock on commit path, BRAVO, Left-Right | this stream informs policy and diagnostics only; best-effort publication keeps it off the commit critical path |
| M8b Witness publication archive | `witness_publication.rs::WitnessPublisher` | two-plane publication: mutex-protected pending reservations plus append-only committed chunks published as immutable history | coarse mutex `Vec`, whole-archive Left-Right, seqlock over archive header | seqlock whole archive, BRAVO, whole-archive Left-Right, RLU-like archive copy | the reserve/write/commit protocol already separates private mutation from public visibility; only the committed side needs reader-friendly immutable publication |

Two design decisions fall out of the matrix:

1. **Seqlock is narrow by design.** It is the right answer only for tiny
   fixed-width snapshots whose readers can cheaply retry, not for ledgers,
   version chains, or ownership state.
2. **RCU is for bounded immutable views, not everything.** The active
   transaction snapshot table wants RCU because SSI scans are long and
   read-dominant. The version store and ownership table do not, because they
   require exact current state or append-only reclamation semantics instead.

The code-level contract anchors that already exist today are:

- `crates/fsqlite-pager/src/pager.rs`: `PAGER_METADATA_PUBLICATION_CONTRACTS`
- `crates/fsqlite-mvcc/src/lib.rs`: `MVCC_METADATA_PUBLICATION_CONTRACTS`

The design-only anchors that this matrix makes explicit for downstream beads
are:

- `crates/fsqlite-mvcc/src/core_types.rs`: `CommitLog`
- `crates/fsqlite-mvcc/src/begin_concurrent.rs`: `ConcurrentRegistry`
- `crates/fsqlite-mvcc/src/witness_publication.rs`: `WitnessPublisher`

---

## E3.3 Visibility, Retry, and Reclamation Contracts

This section turns the E3.2 primitive selection into implementation contracts.
Every metadata class must publish through the selected primitive with the
visibility, retry, and reclamation rules below. A later implementation may tune
layout, sharding, or cache placement, but it must not weaken these rules without
reopening the E3.2 primitive decision.

### Global Contract

1. Writers fully initialize a metadata payload before making it reader-visible.
2. Reader-visible publication uses release ordering or the primitive-specific
   stronger barrier.
3. Readers bind with acquire ordering or the primitive-specific retry protocol.
4. Readers never take a writer mutex on the inline-critical path.
5. Reclamation is delayed until the class-specific reader binding can no longer
   observe the retired object.

### Class Contracts

| Class | Integration Point | Visibility Contract | Retry Contract | Reclamation Contract |
|-------|-------------------|---------------------|----------------|----------------------|
| M1 CommitIndex | `core_types.rs::CommitIndex`; commit publish in `begin_concurrent.rs` | Per-page `commit_seq` becomes visible only after the corresponding version-store entry is installed. Readers may observe stale-low values, never stale-high values. | One acquire load per page; no spin, park, or cross-page retry. | Hot entries are overwritten in place. Cold fallback entries live under shard ownership and may be reclaimed only after no shard reader holds them. |
| M2 Page ownership directory | `core_types.rs::InProcessPageLockTable`; first-touch arbitration | Ownership changes are exact linearizable CAS transitions. Stale reads are not a valid visibility model. | Bounded CAS retry followed by the existing bounded waiter handoff. Readers/writers must not convert this into a snapshot read. | Slots are cleared in place on release. Waiter records are scoped to the bounded handoff queue and die with the wake attempt. |
| M3 Commit sequence counter | `CommitSequenceCombiner`; transaction-manager commit clock | Each writer receives one globally unique sequence before publishing any dependent metadata. | Writers use the combiner, falling back to atomic allocation only through the approved path. Readers consume downstream publications, not the allocator. | No reclamation; monotone scalar. |
| M3b Durable commit ledger | `core_types.rs::CommitLog`; recovery and diagnostics readers | Published records are immutable prefix history. A tail pointer or segment header may advance, but published records are never mutated. | Readers bind a tail generation and resample only if they require newer history. They must not hold the append lock during scans. | Retired segments wait for recovery, diagnostics, and time-travel readers to release their bound generation. |
| M4a Active transaction snapshot table | `begin_concurrent.rs::ConcurrentRegistry` active set | SSI validation binds one immutable active-set image for the validation pass while lifecycle writers continue against mutable leaves. | No seqlock-style long scan retry. A validation pass either uses its bound image or restarts at the transaction validation boundary. | Retired registry images wait for a QSBR grace period before reuse. Per-handle leaves remain separately owned and are not copied into retired images. |
| M4b Committed conflict ledger | `begin_concurrent.rs` committed reader/writer indexes | Conflict history is append-only by key family. Readers see a prefix-stable segment set. | Lookup may resample segment headers between validation passes, but must not retry inside a single exact-key overlap check. | Old immutable segments retire by conflict-history epoch after all active validators that could reference them complete. |
| M5 Pager snapshot summary | `pager.rs::PublishedPagerState` | The summary fields are coherent within one even sequence generation. A reader may bind the previous complete generation. | Retry only while the sequence is odd or changes. If repeated retries exceed the local budget, use the existing bounded park/fallback diagnostics. | No object reclamation; summary is overwritten in place. Lag-detecting page-plane state remains separate. |
| M5b SHM durable snapshot triple | `shm.rs::SharedMemoryLayout` | `(commit_seq, schema_epoch, ecs_epoch)` is a single coherent generation. Independent field sampling is forbidden for durable snapshot binding. | Retry while the seqlock generation is odd or changes. No reader lock. | No reclamation; triple is overwritten in place. |
| M6 Schema invalidation epoch | `lifecycle.rs::schema_epoch`; `shm.rs::load_schema_epoch` | Standalone epoch reads are monotone staleness checks. Durable schema coupling must bind through M5b. | Statement path reads the scalar epoch once and recompiles on mismatch. No schema object RCU publication in this bead. | No global reclamation; schema objects remain connection-local. |
| M7 Version store chain publication | `invariants.rs::VersionStore`; `gc.rs`; `ebr.rs` | Version nodes are initialized and linked before CommitIndex makes their `commit_seq` visible. Readers traverse immutable committed nodes under a guard. | Readers do not retry mutable chains; they traverse the bound chain for their snapshot and treat missing-visible entries as corruption. | Retired nodes go through EBR. GC may prune only below the active horizon and after reader guards can no longer reach the node. |
| M7b GC horizon / reader pins | `core_types.rs::raise_gc_horizon`; `VersionGuardRegistry::min_pinned_epoch` | Published horizon is a safe lower bound. Stale-low is allowed; stale-high is forbidden. | Consumers may recompute or reread on conservative results. They must not require a coherent multi-field snapshot to reclaim. | The horizon itself is not reclaimed. It gates M7 and M4/M4b retire queues. |
| M8 SSI decision telemetry | `ssi_abort_policy.rs`; `observability.rs` | Telemetry is best-effort and never correctness-bearing. Dropped records must be counted. | No retry on the commit path. Policy readers tolerate stale or missing samples. | Bounded ring/offload queue overwrites oldest records. |
| M8b Witness publication archive | `witness_publication.rs::WitnessPublisher` | Pending reservations are private mutable state; committed witnesses become immutable archive chunks. | Readers bind committed chunks. Reservation mutation may use its existing mutex, but committed-history scans must not hold it. | Pending records die with abort/commit. Committed chunks retire by witness-retention epoch after proof replay and diagnostics readers release them. |

### Implementation Handoff

- E3.3.a should implement immutable publication bundles only for classes whose
  contract requires reader-bound immutable history: M3b, M4a, M4b, and M8b.
- E3.3.b should prove the reclamation side: QSBR for M4a, epoch-segment
  retirement for M4b/M8b, and EBR horizon preservation for M7.
- M1, M2, M3, M5, M5b, M6, M7b, and M8 are explicit no-new-primitive classes.
  Their implementation work is limited to preserving the contract and adding
  diagnostics/tests around violations.

---

## Cross-Reference: Work Classification × Metadata Class

This matrix shows which metadata classes are touched by each inline/offload work
class, establishing the coupling between Parts 1 and 2:

| Metadata Class | IC Operations | IF Operations | OA Operations | OB Operations |
|----------------|---------------|---------------|---------------|---------------|
| M1 CommitIndex | First-touch staleness check, batch_update publish | — | — | — |
| M2 PageLockTable | Lock acquire (CAS), lock release (CAS) | Waiter wakeup | — | — |
| M3 next_commit_seq | advance_commit_clock (fetch_add) | — | — | — |
| M4 ConcurrentRegistry | SSI validation (read, under Mutex) | Session recycle | — | — |
| M5 PagerPublishedSnapshot | Snapshot bind (read) | — | — | — |
| M5b ShmSnapshot triple | Snapshot bind / reconcile read | — | — | — |
| M6 Schema epoch | — | — | — | — (read-only on statement path) |
| M7 VersionStore | — | — | — | GC tick |
| M7b GC horizon / reader pins | — | — | — | GC tick, cleanup |
| M8 SSI Evidence | — | — | Evidence recording | — |

**Key insight:** The IC (INLINE-CRITICAL) column shows exactly what must be in
the publish window. M1, M2, M3, and the M4 active snapshot table are the only
metadata classes touched inline-critical. Of these, M4a is the most promising
optimization target because it is the only one still using a coarse Mutex where
an immutable reader snapshot is viable.

---

## Queueing-Theoretic Framing for E4.2

This section sets up the budgeting inputs for the downstream queue-depth and
helper-lane bead (bd-db300.5.4.2).

### Little's Law Application

For the publish window:

```
L = λ · W

where:
  L = average number of writers in the publish window simultaneously
  λ = commit arrival rate (commits/second)
  W = average time in the publish window (seconds)
```

**Measured estimates (from c4 benchmark context):**
- λ ≈ 40,000 commits/sec (c4 disjoint, target)
- W ≈ 1μs (IC operations only, in-memory DB)
- W ≈ 50μs (IC operations, file-backed with WAL fsync)
- Therefore L_memory ≈ 0.04 (publish window is not the bottleneck for in-memory)
- Therefore L_file ≈ 2.0 (at c4, ~2 writers in publish window simultaneously)

For file-backed DBs, the WAL write dominates W. Group commit (batching WAL
writes across concurrent writers) is the primary lever to reduce effective W.

**Backpressure trigger:**
When L > c (where c = number of writers that can physically fit in the publish
window without serialization), additional arrivals must be backpressured at
Stage 4 (first-touch arbitration) rather than allowed to queue at Stage 5.

### Helper-Lane Budget Classes

| Lane | Purpose | Queue Depth Bound | Wake-to-Run Budget | Starvation Policy |
|------|---------|-------------------|--------------------|-------------------|
| **Writer lane** (per-core) | Stages 1–5 IC/IF work | No queue — writer IS the lane | N/A | Fairness via page-lock wait ordering |
| **Wakeup lane** (per-NUMA or shared) | Waiter wakeup dispatch after lock release | Bounded: 2 × max_concurrent_writers | ≤ 10μs | Drain on checkpoint or idle |
| **Evidence lane** (shared) | SSI evidence recording, observability | Bounded: 64 entries | ≤ 1ms | Drop oldest on overflow |
| **GC lane** (shared) | Version chain reclamation | Bounded: 1 outstanding GC sweep | ≤ 100ms | Trigger on chain-depth threshold |
| **Checkpoint lane** (shared) | WAL checkpoint | Bounded: 1 outstanding checkpoint | ≤ 1s | Trigger on WAL size threshold |

### Admission Control Trigger

If the publish window occupancy L exceeds the backpressure threshold:

1. **First response:** Delay new transaction BEGIN by parking in admission queue.
2. **Second response:** If admission queue depth > 2 × core_count, reject new
   transactions with SQLITE_BUSY.
3. **Fallback:** Under sustained overload, checkpoint lane drains WAL to reduce
   per-commit W, which reduces L.

This ensures p50 latency is protected: ordinary commits never wait behind
a backlog of deferred work.

---

## Verification Plan

### Unit Tests Required

1. **IC classification correctness:** Instrument the publish window with
   timestamps. Assert that no OA or OB operation executes between SSI validation
   start and lock release completion.

2. **Offload safety:** For each OA/OB operation, test that commit is visible to
   readers BEFORE the offloaded operation completes. This proves the offload
   does not violate INV-E1.1-5.

3. **Metadata monotonicity:** For M1 (CommitIndex), property-test that
   batch_update never publishes a sequence number lower than the existing value
   for any page.

4. **Seqlock snapshot coherence:** Stress `PublishedPagerState::snapshot()` and
   `SharedMemoryLayout::load_consistent_snapshot()` under concurrent publish and
   assert readers never observe torn field combinations.

5. **RCU registry grace safety:** When the M4 implementation lands, verify that
   an SSI scan can finish against a retired registry image while writers
   continue publishing newer images, and that recycle only happens after a
   grace period.

### E2E Scenarios Required

1. **c4 publish-window measurement:** Run the c4 disjoint benchmark with
   publish-window timing instrumentation. Capture p50/p95/p99 of W.

2. **Registry contention measurement:** Run the c4 mixed benchmark with
   ConcurrentRegistry lock-hold timing. Capture contention rate and p99 wait.

3. **Offload queue depth monitoring:** Run sustained c8 workload and verify
   that OA/OB queues stay within their depth bounds.

4. **Topology-sensitive publication interference:** Run c4 and c8 mixed
   workloads while capturing pager publication retry counts, SHM seqlock retry
   counts, and any future RCU grace-period lag so primitive choice can be tied
   back to real interference rather than microbench aesthetics.

### Logging Artifacts Required

- `tracing::debug!(target: "fsqlite::commit::classify", op = %name, class = %class, duration_ns = %dur)`
- `tracing::info!(target: "fsqlite::commit::publish_window", writers_in_window = %l, window_duration_ns = %w)`
- `tracing::warn!(target: "fsqlite::commit::backpressure", trigger = %reason, queue_depth = %depth)`

---

## Assumptions Ledger

| ID | Assumption | Verification Method | Failure Mode |
|----|-----------|---------------------|--------------|
| A1 | WAL write dominates publish window for file-backed DBs | Profile txn.commit() duration breakdown | If false, atomic operations dominate — focus on combiner optimization |
| A2 | ConcurrentRegistry Mutex is the primary c4 contention source | Lock-hold timing + contention rate measurement | If false, CommitIndex or PageLockTable contention dominates — change E3.2 priority |
| A3 | Waiter wakeup can be safely moved outside the publish window | Test: writer releases lock, reader sees committed state before wakeup fires | If false, some reader depends on synchronous wakeup — keep wakeup in IC |
| A4 | RCU-style snapshot is viable for ConcurrentRegistry at ≤ 64 concurrent sessions | Prototype + benchmark at c4/c8 | If false, session count exceeds RCU copy budget — fall back to sharded Mutex |
| A5 | seqlock-style pager summary publication eliminates Mutex contention on snapshot bind | Before/after c4 BEGIN latency measurement | If false, other BEGIN work dominates — deprioritize M5 change |

---

## Consequences for Downstream Beads

| Downstream Bead | What This Artifact Provides |
|-----------------|---------------------------|
| **bd-db300.5.4.2** (E4.2: queue budgets) | Lane definitions, queue depth bounds, Little's Law parameters, backpressure trigger formula |
| **bd-db300.5.3.2** (E3.2: primitive mapping) | Selected and rejected primitive matrix, split of active snapshot vs committed ledger, coupling constraints |
| **bd-db300.5.4.3** (E4.3: admission control) | Admission control trigger sequence, p50 protection mechanism |
| **bd-77l3t** (HTM fast-path) | M3 classification confirms CommitSequenceCombiner is the HTM target |
| **bd-3t52f** (DRO abort policy) | M8 classification confirms evidence is OA/fire-and-forget, safe for policy input |
| **bd-wnk1r / bd-bolsv** (version chain GC) | M7 classification confirms EBR discipline, GC is OB class |
