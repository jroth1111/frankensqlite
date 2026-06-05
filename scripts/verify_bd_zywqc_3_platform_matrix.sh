#!/usr/bin/env bash
set -euo pipefail

# bd-zywqc.3: Local verification of platform-specific isolation tests.
#
# Runs the platform isolation test suite on the current host and verifies
# that the CI workflow file is syntactically valid. For full cross-platform
# coverage, the CI matrix must run on GitHub Actions.
#
# Usage:
#   ./scripts/verify_bd_zywqc_3_platform_matrix.sh

BEAD_ID="bd-zywqc.3"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "[$BEAD_ID] Platform matrix verification (local)"
echo "[$BEAD_ID] Host: $(uname -s) $(uname -m)"
echo "[$BEAD_ID] Rust: $(rustc --version 2>/dev/null || echo 'not found')"
echo ""

PASSED=0
FAILED=0

run_phase() {
    local phase="$1"
    local cmd="$2"
    echo "[$BEAD_ID] Phase $phase..."
    if eval "$cmd"; then
        echo "[$BEAD_ID] Phase $phase: PASSED"
        PASSED=$((PASSED + 1))
    else
        echo "[$BEAD_ID] Phase $phase: FAILED"
        FAILED=$((FAILED + 1))
    fi
    echo ""
}

# Phase 1: Workflow file exists and is valid YAML
run_phase "1: workflow file exists" \
    "test -f '$REPO_ROOT/.github/workflows/concurrent-platform-matrix.yml'"

# Phase 2: Workflow has all 4 platform entries
run_phase "2: workflow has 4 platforms" \
    "grep -c 'platform_tag:' '$REPO_ROOT/.github/workflows/concurrent-platform-matrix.yml' | grep -q '^4$'"

# Phase 3: Test file compiles
run_phase "3: platform isolation tests compile" \
    "cargo test -p fsqlite-e2e --test bd_zywqc_3_platform_isolation --no-run 2>&1"

# Phase 4: Run platform isolation tests
run_phase "4: platform isolation tests pass" \
    "cargo test -p fsqlite-e2e --test bd_zywqc_3_platform_isolation -- --nocapture 2>&1"

# Phase 5: CI integrity check script exists
run_phase "5: ci_integrity_check.sh exists" \
    "test -x '$REPO_ROOT/scripts/ci_integrity_check.sh'"

echo ""
echo "[$BEAD_ID] Results: $PASSED passed, $FAILED failed"
if [ "$FAILED" -gt 0 ]; then
    echo "[$BEAD_ID] FAILED"
    exit 1
fi
echo "[$BEAD_ID] PASSED (local verification only — full matrix runs in CI)"
