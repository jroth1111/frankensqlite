#!/usr/bin/env bash
set -euo pipefail

# bd-zywqc.4: Local semver-check verification script.
#
# Runs cargo semver-checks against main HEAD for all enforced crates.
# Exit code: 0 if no breaks, 1 if breaks detected.
#
# Usage:
#   ./scripts/verify_bd_zywqc_4_semver_check.sh [--baseline-rev REV]
#
# Requires: cargo-semver-checks (cargo install cargo-semver-checks)

BEAD_ID="bd-zywqc.4"
BASELINE_REV="main"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --baseline-rev) BASELINE_REV="$2"; shift 2 ;;
        *) echo "Unknown option: $1" >&2; exit 2 ;;
    esac
done

ENFORCED_CRATES=(
    fsqlite
    fsqlite-core
    fsqlite-vfs
    fsqlite-vdbe
    fsqlite-mvcc
    fsqlite-wal
    fsqlite-pager
)

# Exempt crates (documented for reference):
# fsqlite-harness, fsqlite-observability, fsqlite-e2e, fsqlite-ext-*,
# fsqlite-btree, fsqlite-types, fsqlite-error, fsqlite-parser

echo "[$BEAD_ID] Public API stability check"
echo "[$BEAD_ID] Baseline: $BASELINE_REV"
echo "[$BEAD_ID] Enforced crates: ${ENFORCED_CRATES[*]}"
echo ""

if ! command -v cargo-semver-checks &>/dev/null; then
    echo "[$BEAD_ID] ERROR: cargo-semver-checks not installed"
    echo "[$BEAD_ID] Install: cargo install cargo-semver-checks"
    exit 2
fi

PASSED=0
FAILED=0
SKIPPED=0

for crate in "${ENFORCED_CRATES[@]}"; do
    echo -n "  [$crate] ... "
    if cargo semver-checks check-release \
        --package "$crate" \
        --baseline-rev "$BASELINE_REV" >/dev/null 2>&1; then
        echo "ok"
        PASSED=$((PASSED + 1))
    else
        EXIT_CODE=$?
        if [[ $EXIT_CODE -eq 2 ]]; then
            echo "skipped (not published)"
            SKIPPED=$((SKIPPED + 1))
        else
            echo "BREAKING CHANGE DETECTED"
            FAILED=$((FAILED + 1))
        fi
    fi
done

echo ""
echo "[$BEAD_ID] Results: $PASSED passed, $FAILED failed, $SKIPPED skipped"

if [[ $FAILED -gt 0 ]]; then
    echo "[$BEAD_ID] FAILED — breaking API changes detected"
    echo "[$BEAD_ID] To override: add [breaking] to PR title + docs/rfc/BREAKING-<slug>.md"
    exit 1
fi

echo "[$BEAD_ID] PASSED — no breaking API changes"
