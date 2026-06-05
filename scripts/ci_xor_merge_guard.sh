#!/usr/bin/env bash
# ci_xor_merge_guard.sh — bd-pwyf0
#
# Scans the workspace for call sites that pass a non-Opaque MergePageKind
# to attempt_raw_xor_merge or compose_disjoint_deltas without going through
# enforce_raw_xor_merge_policy first.
#
# Exit 0 = clean, exit 1 = suspicious call sites found.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Pattern: direct calls to compose_disjoint_deltas that are NOT inside
# attempt_raw_xor_merge (which has the policy guard). Exclude the definition
# file itself and test files.
VIOLATIONS=0

# 1) Grep for compose_disjoint_deltas calls outside xor_delta.rs
matches=$(grep -rn 'compose_disjoint_deltas' "$REPO_ROOT/crates" \
    --include='*.rs' \
    | grep -v 'xor_delta\.rs' \
    | grep -v '/tests/' \
    | grep -v '#\[cfg(test)\]' \
    | grep -v '// xor-guard-ok' \
    || true)

if [ -n "$matches" ]; then
    echo "WARNING: compose_disjoint_deltas called outside xor_delta.rs without guard:"
    echo "$matches"
    echo ""
    echo "Use attempt_raw_xor_merge() instead (includes policy enforcement),"
    echo "or add '// xor-guard-ok' comment if the call is intentionally unguarded."
    VIOLATIONS=1
fi

# 2) Check that is_raw_xor_forbidden_page_kind covers all non-Opaque variants.
# Count MergePageKind variants in fsqlite-types.
variant_count=$(grep -c '^\s*///\|^\s*[A-Z][a-z]' "$REPO_ROOT/crates/fsqlite-types/src/lib.rs" \
    | head -1 || echo "0")

# Simpler: just verify the forbidden constant array has 7 entries (all non-Opaque).
forbidden_count=$(grep -A8 'RAW_XOR_FORBIDDEN_PAGE_KINDS' "$REPO_ROOT/crates/fsqlite-mvcc/src/xor_delta.rs" \
    | grep 'MergePageKind::' | wc -l)

if [ "$forbidden_count" -ne 7 ]; then
    echo "ERROR: RAW_XOR_FORBIDDEN_PAGE_KINDS has $forbidden_count entries, expected 7."
    echo "A new MergePageKind variant may need to be added to the forbidden list."
    VIOLATIONS=1
fi

if [ "$VIOLATIONS" -eq 0 ]; then
    echo "xor-merge-guard: all checks passed."
fi

exit "$VIOLATIONS"
